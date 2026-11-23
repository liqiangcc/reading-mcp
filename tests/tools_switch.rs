//! `READING_MCP_TOOLS` accepts only the v2 tool set: unset and `v2` serve the seven tools;
//! `v1`, `both` and any other value refuse to start, on both binaries, before serving.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use std::process::Stdio;

use tempfile::tempdir;
use tokio::process::Command;

const V2_TOOLS: [&str; 7] = [
    "browse",
    "cite",
    "open",
    "outline",
    "read",
    "search",
    "source_view",
];

async fn assert_seven_v2_tools(client: &v2_mcp::Client) {
    let tools = client
        .list_all_tools()
        .await
        .expect("tools/list should answer");
    let mut names: Vec<String> = tools.iter().map(|tool| tool.name.to_string()).collect();
    names.sort();
    assert_eq!(names, V2_TOOLS.map(str::to_string).to_vec());
}

#[tokio::test]
async fn an_unset_tools_value_serves_the_seven_v2_tools() {
    let directory = tempdir().expect("temp directory");
    let client = v2_mcp::start_client(directory.path(), &directory.path().join("state")).await;
    assert_seven_v2_tools(&client).await;
    client.cancel().await.expect("client should stop");
}

#[tokio::test]
async fn an_explicit_v2_tools_value_serves_the_seven_v2_tools() {
    let directory = tempdir().expect("temp directory");
    let client = v2_mcp::start_client_with_env(
        directory.path(),
        &directory.path().join("state"),
        &[("READING_MCP_TOOLS", "v2")],
    )
    .await;
    assert_seven_v2_tools(&client).await;
    client.cancel().await.expect("client should stop");
}

#[tokio::test]
async fn an_unset_tools_value_lets_the_http_server_serve() {
    let directory = tempdir().expect("temp directory");
    let port = v2_mcp::reserve_port();
    let base = format!("http://127.0.0.1:{port}");
    let mut child =
        v2_mcp::http_server_command(port, directory.path(), &directory.path().join("state"))
            .spawn()
            .expect("HTTP server should spawn");
    v2_mcp::wait_until_ready(&base, &mut child).await;
    let client = v2_mcp::start_http_client(&base).await;
    assert_seven_v2_tools(&client).await;
    client.cancel().await.expect("client should stop");
    child.kill().await.expect("HTTP server should stop");
}

#[tokio::test]
async fn stale_tools_values_refuse_to_start_on_both_binaries() {
    let directory = tempdir().expect("temp directory");
    for value in ["v1", "both", "V1", "v3", "garbage"] {
        for (binary, path) in [
            ("reading-mcp", env!("CARGO_BIN_EXE_reading-mcp")),
            ("reading-mcp-http", env!("CARGO_BIN_EXE_reading-mcp-http")),
        ] {
            let mut command = Command::new(path);
            command
                .env("READING_MCP_TOOLS", value)
                .env("READING_MCP_LOCAL_ROOTS", directory.path())
                .env("READING_MCP_STATE_DIR", directory.path().join("state"))
                .env("READING_MCP_TELEMETRY", "false")
                .env("READING_MCP_HTTP_TOKEN", v2_mcp::HTTP_TOKEN)
                .env("READING_MCP_HTTP_BIND", "127.0.0.1:0")
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            let output = command
                .output()
                .await
                .unwrap_or_else(|error| panic!("{binary} should exit: {error}"));
            assert!(
                !output.status.success(),
                "{binary} must refuse READING_MCP_TOOLS={value}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("READING_MCP_TOOLS must be one of v2"),
                "{binary} READING_MCP_TOOLS={value} stderr: {stderr}"
            );
        }
    }
}
