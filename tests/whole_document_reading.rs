use std::collections::HashSet;

use reading_mcp::mcp::contracts::{
    GetContextResponse, GetDocumentStructureResponse, GetTextUnitsResponse, OpenDocumentResponse,
};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

#[tokio::test]
async fn stdio_composes_all_sections_in_body_order_with_truthful_completion() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let document_path = directory.path().join("book.md");
    tokio::fs::write(
        &document_path,
        "# Book\n\nIntroduction.\n\n## Duplicate\n\nParent sentence. Child-independent sentence.\n\n### Empty\n\n## Duplicate\n\nSibling sentence.\n",
    )
    .await
    .expect("book fixture should be written");

    let client = start_client(directory.path(), "memory").await;
    let opened = open(&client, &document_path).await;
    let structure = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "max_nodes": 10
            }))),
        )
        .await
        .expect("structure should succeed")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("structure response should deserialize");

    assert!(structure.complete);
    assert_eq!(structure.stream.body_order_version, "body-order/v1");
    let mut sections = flatten_sections(&structure);
    sections.sort_by_key(|section| section.body_order);
    assert_eq!(
        sections
            .iter()
            .map(|section| section.body_order)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        sections
            .iter()
            .map(|section| section.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Book", "Duplicate", "Empty", "Duplicate"]
    );

    let mut consumed = Vec::new();
    let mut consumed_ids = HashSet::<String>::new();
    for section in sections {
        let mut cursor = None;
        loop {
            let response = client
                .call_tool(
                    CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                        "document_id": structure.document_id,
                        "section_id": section.section_id,
                        "requested_kind": "sentence",
                        "direction": "forward",
                        "coverage_policy": "preserve_source",
                        "max_items": 1,
                        "cursor": cursor
                    }))),
                )
                .await
                .expect("section text-unit page should succeed")
                .into_typed::<GetTextUnitsResponse>()
                .expect("text-unit response should deserialize");
            assert!(response.coverage.source_complete);
            for item in &response.items {
                let locator_key =
                    serde_json::to_string(&item.locator).expect("locator is serializable");
                assert!(consumed_ids.insert(locator_key));
                consumed.push((section.body_order, item.locator.owner_section_id.clone()));
            }
            if response.complete {
                assert!(response.next_cursor.is_none());
                assert!(response.section_complete);
                break;
            }
            cursor = response.next_cursor;
            assert!(cursor.is_some());
        }
    }

    assert_eq!(
        consumed
            .iter()
            .map(|(_, section_id)| section_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "section://book",
            "section://book/duplicate",
            "section://book/duplicate",
            "section://book/duplicate-2"
        ]
    );
    assert_eq!(
        consumed.iter().map(|(order, _)| *order).collect::<Vec<_>>(),
        [0, 1, 1, 3]
    );

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn saved_text_locator_resumes_after_mcp_restart_and_questions_do_not_advance() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state_dir = directory.path().join("state");
    let document_path = directory.path().join("resume.md");
    tokio::fs::write(
        &document_path,
        "# Resume\n\nFirst sentence. Second sentence.\n\n## Next\n\nThird sentence.\n",
    )
    .await
    .expect("resume fixture should be written");

    let first_client = start_client(directory.path(), &state_dir).await;
    let opened = open(&first_client, &document_path).await;
    let first = call_units(
        &first_client,
        &opened.document_id,
        "section://resume",
        json!({
            "requested_kind": "sentence",
            "coverage_policy": "preserve_source",
            "max_items": 1
        }),
    )
    .await;
    assert_eq!(first.items[0].text, "First sentence.");

    let observational_context = first_client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "section_id": "section://resume",
                "before": 0,
                "after": 1
            }))),
        )
        .await
        .expect("context inspection should succeed")
        .into_typed::<GetContextResponse>()
        .expect("context response should deserialize");
    assert!(observational_context.content.contains("First sentence."));

    let repeated = call_units(
        &first_client,
        &opened.document_id,
        "section://resume",
        json!({
            "requested_kind": "sentence",
            "coverage_policy": "preserve_source",
            "max_items": 1
        }),
    )
    .await;
    assert_eq!(repeated.items[0].text, "First sentence.");
    let saved_locator = first.items[0].locator.clone();
    first_client
        .cancel()
        .await
        .expect("first client should stop");

    let second_client = start_client(directory.path(), &state_dir).await;
    let reopened = open(&second_client, &document_path).await;
    assert_eq!(reopened.document_id, opened.document_id);
    let resumed = second_client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": reopened.document_id,
                "section_id": "section://resume",
                "anchor_locator": saved_locator,
                "requested_kind": "sentence",
                "direction": "forward",
                "coverage_policy": "preserve_source",
                "max_items": 1
            }))),
        )
        .await
        .expect("saved locator should resume after restart")
        .into_typed::<GetTextUnitsResponse>()
        .expect("resumed response should deserialize");
    assert_eq!(resumed.items[0].text, "Second sentence.");
    assert!(resumed.start_anchor_locator.is_some());

    second_client
        .cancel()
        .await
        .expect("second client should stop");
}

async fn open(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    path: &std::path::Path,
) -> OpenDocumentResponse {
    client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": path.to_string_lossy()
            }))),
        )
        .await
        .expect("open_document should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("open response should deserialize")
}

async fn call_units(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    document_id: &str,
    section_id: &str,
    extra: Value,
) -> GetTextUnitsResponse {
    let mut value = serde_json::Map::new();
    value.insert("document_id".into(), json!(document_id));
    value.insert("section_id".into(), json!(section_id));
    if let Some(extra) = extra.as_object() {
        value.extend(extra.clone());
    }
    client
        .call_tool(CallToolRequestParams::new("get_text_units").with_arguments(value))
        .await
        .expect("get_text_units should succeed")
        .into_typed::<GetTextUnitsResponse>()
        .expect("text-unit response should deserialize")
}

async fn start_client(
    local_root: &std::path::Path,
    state_dir: impl AsRef<std::path::Path>,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let local_roots = std::env::join_paths([local_root]).expect("local roots should be valid");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", state_dir.as_ref())
        .env("READING_MCP_TELEMETRY", "false");
    let transport = TokioChildProcess::new(command).expect("MCP process should start");
    ().serve(transport)
        .await
        .expect("MCP initialization should succeed")
}

fn flatten_sections(
    response: &GetDocumentStructureResponse,
) -> Vec<&reading_mcp::mcp::contracts::SectionNode> {
    fn collect<'a>(
        section: &'a reading_mcp::mcp::contracts::SectionNode,
        output: &mut Vec<&'a reading_mcp::mcp::contracts::SectionNode>,
    ) {
        output.push(section);
        for child in &section.children {
            collect(child, output);
        }
    }
    let mut output = Vec::new();
    for section in &response.sections {
        collect(section, &mut output);
    }
    output
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be an object")
        .clone()
}
