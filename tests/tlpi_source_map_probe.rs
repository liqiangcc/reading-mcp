use std::path::{Path, PathBuf};

use reading_mcp::mcp::contracts::{
    GetDocumentStructureResponse, OpenDocumentResponse, ReadDocumentResponse,
    SearchDocumentResponse, SectionNode,
};
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use serde_json::{json, Map, Value};
use tokio::process::Command;
use url::Url;

const DOCUMENT_ID: &str =
    "doc:sha256:286e0104a40d05c3cb76f08e2d6a06391ce9d1bc603351aefc2340aca3349b2f";
const OUTPUT: &str = "tlpi-source-map-probe.json";

#[tokio::test]
async fn collect_tlpi_source_map_through_reading_mcp() {
    let state_dir = std::env::var("TLPI_READING_STATE_DIR")
        .unwrap_or_else(|_| "/root/.reading-mcp".to_owned());
    let local_roots =
        std::env::var("TLPI_READING_LOCAL_ROOTS").unwrap_or_else(|_| "/root".to_owned());

    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_STATE_DIR", &state_dir)
        .env("READING_MCP_LOCAL_ROOTS", &local_roots)
        .env("READING_MCP_TELEMETRY", "false");

    let transport = TokioChildProcess::new(command).expect("reading-mcp child process should start");
    let client = ().serve(transport).await.expect("Reading MCP initialize should succeed");

    let mut tool_names = client
        .list_all_tools()
        .await
        .expect("tools/list should succeed")
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
    tool_names.sort();

    let structure = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": DOCUMENT_ID,
                "max_depth": 32
            }))),
        )
        .await
        .expect("get_document_structure should reach the persisted TLPI document")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("structure response should be typed");

    let mut flat_nodes = Vec::new();
    flatten(&structure.sections, &mut flat_nodes);

    let preface_node = flat_nodes
        .iter()
        .find(|node| title_contains(&node.title, &["前言", "序言", "preface"]))
        .cloned();

    let metadata_nodes = flat_nodes
        .iter()
        .filter(|node| {
            title_contains(
                &node.title,
                &[
                    "版权", "copyright", "书名", "title page", "出版", "扉页", "前言", "序言",
                    "preface",
                ],
            )
        })
        .take(12)
        .cloned()
        .collect::<Vec<_>>();

    let mut structural_reads = Vec::new();
    let mut opened: Option<OpenDocumentResponse> = None;
    let mut source_basename: Option<String> = None;

    for node in &metadata_nodes {
        let read = client
            .call_tool(
                CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                    "document_id": DOCUMENT_ID,
                    "section_id": node.section_id,
                    "max_chars": 64000
                }))),
            )
            .await
            .expect("structural metadata section should be readable")
            .into_typed::<ReadDocumentResponse>()
            .expect("read response should be typed");

        if opened.is_none() {
            if let Some(path) = source_to_path(&read.source) {
                source_basename = path.file_name().map(|name| name.to_string_lossy().into_owned());
                opened = Some(
                    client
                        .call_tool(
                            CallToolRequestParams::new("open_document").with_arguments(arguments(
                                json!({
                                    "source": path.to_string_lossy(),
                                    "force_refresh": false
                                }),
                            )),
                        )
                        .await
                        .expect("open_document should reopen the persisted local EPUB")
                        .into_typed::<OpenDocumentResponse>()
                        .expect("open response should be typed"),
                );
            }
        }

        structural_reads.push(json!({
            "node": node,
            "read": read,
        }));
    }

    let mut searches = Vec::new();
    for query in [
        "本书的目标 读者",
        "本书的组织结构 章节",
        "基础部分 后续章节",
        "版本 版次 ISBN",
    ] {
        let response = client
            .call_tool(
                CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                    "document_id": DOCUMENT_ID,
                    "query": query,
                    "limit": 12
                }))),
            )
            .await
            .expect("structural search should succeed")
            .into_typed::<SearchDocumentResponse>()
            .expect("search response should be typed");
        searches.push(json!({"query": query, "response": response}));
    }

    let mut output = json!({
        "probe_version": 3,
        "document_id": DOCUMENT_ID,
        "state_dir_exists": Path::new(&state_dir).exists(),
        "local_roots": "<redacted-local-root>",
        "tools": tool_names,
        "open_document": opened,
        "source_basename": source_basename,
        "structure": structure,
        "flattened_node_count": flat_nodes.len(),
        "preface_node": preface_node,
        "structural_reads": structural_reads,
        "structural_searches": searches,
    });
    scrub_sources(&mut output);

    std::fs::write(
        OUTPUT,
        serde_json::to_vec_pretty(&output).expect("probe JSON should serialize"),
    )
    .expect("probe artifact should be written");

    client.cancel().await.expect("Reading MCP child should close cleanly");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}

fn flatten(nodes: &[SectionNode], output: &mut Vec<SectionNode>) {
    for node in nodes {
        output.push(node.clone());
        flatten(&node.children, output);
    }
}

fn title_contains(title: &str, needles: &[&str]) -> bool {
    let normalized = title.to_lowercase();
    needles.iter().any(|needle| normalized.contains(&needle.to_lowercase()))
}

fn source_to_path(source: &str) -> Option<PathBuf> {
    if let Ok(url) = Url::parse(source) {
        if url.scheme() == "file" {
            return url.to_file_path().ok();
        }
    }
    let path = PathBuf::from(source);
    path.is_absolute().then_some(path)
}

fn scrub_sources(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "source" {
                    if let Value::String(source) = child {
                        if let Some(path) = source_to_path(source) {
                            let basename = path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "unknown".to_owned());
                            *source = format!("file://<local>/{}", basename);
                        }
                    }
                } else {
                    scrub_sources(child);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                scrub_sources(child);
            }
        }
        _ => {}
    }
}
