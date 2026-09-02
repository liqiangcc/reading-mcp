use reading_mcp::mcp::contracts::{
    OpenDocumentResponse, ReadingCapabilityAvailabilityDto, ReliabilityIntegrityDto,
};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

#[tokio::test]
async fn stdio_open_returns_reading_profile_without_expanding_tool_surface() {
    let directory = tempfile::tempdir().expect("temporary document directory should be created");
    let document_path = directory.path().join("profile.md");
    tokio::fs::write(
        &document_path,
        "# Profile\n\nFirst sentence. Second sentence.\n",
    )
    .await
    .expect("Markdown fixture should be written");

    let local_roots = std::env::join_paths([directory.path()])
        .expect("temporary directory should be a valid local root list");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", "memory")
        .env("READING_MCP_TELEMETRY", "false");
    let transport =
        TokioChildProcess::new(command).expect("reading-mcp child process should start");
    let client = ().serve(transport).await.expect("MCP initialization should succeed");

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
            "get_context",
            "get_document_structure",
            "get_text_units",
            "list_directory",
            "list_documents",
            "open_document",
            "read_document",
            "search_document",
        ]
    );

    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": document_path.to_string_lossy(),
                "force_refresh": false
            }))),
        )
        .await
        .expect("open_document MCP call should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("open_document should return typed structured content");

    assert_eq!(opened.reading_profile.schema_version, "reading-profile/v1");
    assert_eq!(
        opened
            .reading_profile
            .capabilities
            .paragraph_enumeration
            .availability,
        ReadingCapabilityAvailabilityDto::Available
    );
    assert_eq!(
        opened
            .reading_profile
            .capabilities
            .paragraph_enumeration
            .segmentation_version,
        "text-segmentation/v2"
    );
    assert_eq!(
        opened
            .reading_profile
            .capabilities
            .lexical_search
            .availability,
        ReadingCapabilityAvailabilityDto::Available
    );
    assert!(
        opened
            .reading_profile
            .capabilities
            .lexical_search
            .precise_candidates
    );

    let coverage = &opened.reading_profile.canonical_text_coverage;
    assert!(coverage.owner_chars > 0);
    assert_eq!(
        coverage.paragraph_chars + coverage.paragraph_separator_chars,
        coverage.owner_chars
    );
    assert_eq!(coverage.native_paragraph_chars, 0);
    assert_eq!(coverage.native_structural_container_chars, 0);
    assert_eq!(coverage.native_non_prose_chars, 0);
    assert!(coverage.fallback_chars > 0);
    assert_eq!(coverage.coarse_paragraphs, 0);
    assert!(
        !opened
            .reading_profile
            .capabilities
            .sentence_first_enumeration
            .source_preserving_coarse_regions
    );

    assert_eq!(opened.reading_profile.reliability.evidence.len(), 1);
    assert_eq!(
        opened.reading_profile.reliability.evidence[0].integrity,
        ReliabilityIntegrityDto::NotApplicable
    );
    assert!(
        opened
            .reading_profile
            .reliability
            .publication_coverage
            .is_none()
    );
    assert!(
        opened
            .reading_profile
            .reliability
            .structure_provenance
            .is_none()
    );
    assert!(
        opened
            .reading_profile
            .reliability
            .navigation_resolution
            .is_none()
    );

    client
        .cancel()
        .await
        .expect("MCP child process should close cleanly");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}
