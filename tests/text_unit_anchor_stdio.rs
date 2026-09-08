use reading_mcp::mcp::contracts::{
    GetDocumentStructureResponse, GetTextUnitsResponse, OpenDocumentResponse,
};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

#[tokio::test]
async fn stdio_get_text_units_continues_exclusively_after_locator_anchor() {
    let directory = tempfile::tempdir().expect("temporary document directory");
    let document_path = directory.path().join("anchor.md");
    tokio::fs::write(
        &document_path,
        "# Root\n\nOne. Two. Three. Four.\n\n# Next\n\nDo not enumerate this section.\n",
    )
    .await
    .expect("fixture write");

    let local_roots = std::env::join_paths([directory.path()]).expect("local roots");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", "memory")
        .env("READING_MCP_TELEMETRY", "false");
    let transport = TokioChildProcess::new(command).expect("server process");
    let client = ().serve(transport).await.expect("MCP initialization");

    let tools = client.list_all_tools().await.expect("tools/list");
    let tool = tools
        .iter()
        .find(|tool| tool.name == "get_text_units")
        .expect("get_text_units tool");
    assert!(
        tool.description
            .as_deref()
            .unwrap()
            .contains("complete=true")
    );
    let schema = serde_json::to_value(tool.output_schema.as_ref().expect("output schema"))
        .expect("schema JSON");
    assert!(
        schema["properties"]["complete"]["description"]
            .as_str()
            .unwrap()
            .contains("directional Section boundary")
    );
    assert!(
        schema["properties"]["section_complete"]["description"]
            .as_str()
            .unwrap()
            .contains("Always false")
    );

    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": document_path.to_string_lossy()
            }))),
        )
        .await
        .expect("open_document")
        .into_typed::<OpenDocumentResponse>()
        .expect("typed open response");

    let structure = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "max_depth": 2
            }))),
        )
        .await
        .expect("get_document_structure")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("typed structure response");
    let section_id = structure.sections[0].section_id.clone();

    let initial = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": structure.document_id,
                "section_id": section_id,
                "requested_kind": "sentence",
                "direction": "forward",
                "coverage_policy": "preserve_source",
                "max_items": 10
            }))),
        )
        .await
        .expect("initial enumeration")
        .into_typed::<GetTextUnitsResponse>()
        .expect("typed initial enumeration");
    assert_eq!(initial.items.len(), 4);
    let anchor = initial.items[1].locator.clone();
    let anchor_json = serde_json::to_value(&anchor).expect("anchor JSON");

    let anchored = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": initial.document_id,
                "section_id": section_id,
                "anchor_locator": anchor_json,
                "requested_kind": "sentence",
                "direction": "forward",
                "coverage_policy": "preserve_source",
                "max_items": 1
            }))),
        )
        .await
        .expect("anchored enumeration")
        .into_typed::<GetTextUnitsResponse>()
        .expect("typed anchored enumeration");
    assert_eq!(anchored.items.len(), 1);
    assert_eq!(anchored.items[0].text, "Three.");
    assert_eq!(anchored.start_anchor_locator.as_ref(), Some(&anchor));
    assert!(!anchored.complete);
    assert!(!anchored.section_complete);
    let cursor = anchored.next_cursor.expect("anchored cursor");

    let continued = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": anchored.document_id,
                "section_id": section_id,
                "requested_kind": "sentence",
                "direction": "forward",
                "coverage_policy": "preserve_source",
                "max_items": 10,
                "cursor": cursor
            }))),
        )
        .await
        .expect("anchored continuation")
        .into_typed::<GetTextUnitsResponse>()
        .expect("typed anchored continuation");
    assert_eq!(continued.items.len(), 1);
    assert_eq!(continued.items[0].text, "Four.");
    assert_eq!(continued.start_anchor_locator.as_ref(), Some(&anchor));
    assert!(continued.complete);
    assert!(!continued.section_complete);
    assert!(continued.next_cursor.is_none());

    // No call targets the next Section: the last anchor alone proves directional EOS.
    for kind in ["sentence", "paragraph"] {
        let last_anchor = if kind == "sentence" {
            continued.items[0].locator.clone()
        } else {
            client
                .call_tool(
                    CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                        "document_id": initial.document_id,
                        "section_id": section_id,
                        "requested_kind": kind,
                        "direction": "backward",
                        "coverage_policy": "preserve_source",
                        "max_items": 1
                    }))),
                )
                .await
                .expect("last paragraph")
                .into_typed::<GetTextUnitsResponse>()
                .expect("paragraph response")
                .items[0]
                .locator
                .clone()
        };
        for _ in 0..2 {
            let eos = client
                .call_tool(
                    CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                        "document_id": initial.document_id,
                        "section_id": section_id,
                        "anchor_locator": last_anchor,
                        "requested_kind": kind,
                        "direction": "forward",
                        "coverage_policy": "preserve_source",
                        "max_items": 1
                    }))),
                )
                .await
                .expect("terminal anchor")
                .into_typed::<GetTextUnitsResponse>()
                .expect("EOS response");
            assert!(eos.items.is_empty());
            assert!(eos.next_cursor.is_none());
            assert!(eos.complete);
            assert!(!eos.section_complete);
            assert!(eos.coverage.source_complete);
            assert_eq!(eos.coverage.unsupported_gaps, 0);
            assert_eq!(eos.stream.start_index, eos.stream.total_items);
            assert_eq!(eos.stream.end_index, eos.stream.total_items);
            assert_eq!(eos.start_anchor_locator.as_ref(), Some(&last_anchor));
        }
    }

    client.cancel().await.expect("client shutdown");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments must be an object")
        .clone()
}
