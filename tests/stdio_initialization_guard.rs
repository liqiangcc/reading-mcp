use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdout, Command};
use tokio::time::timeout;

#[tokio::test]
async fn preinitialize_tool_call_is_rejected_without_terminating_stdio_server() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_STATE_DIR", "memory")
        .env("READING_MCP_TELEMETRY", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .expect("reading-mcp child process should start");
    let mut stdin = child
        .stdin
        .take()
        .expect("reading-mcp stdin should be piped");
    let stdout = child
        .stdout
        .take()
        .expect("reading-mcp stdout should be piped");
    let mut lines = BufReader::new(stdout).lines();

    send_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 41,
            "method": "tools/call",
            "params": {
                "name": "list_documents",
                "arguments": {}
            }
        }),
    )
    .await;

    let rejected = receive_message(&mut lines).await;
    assert_eq!(rejected["id"], 41);
    assert_eq!(rejected["error"]["code"], -32600);
    assert_eq!(rejected["error"]["message"], "Server not initialized");
    assert!(
        child
            .try_wait()
            .expect("reading-mcp process status should be readable")
            .is_none(),
        "reading-mcp must remain alive after rejecting a pre-initialize tool call"
    );

    send_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": "stdio-initialization-guard-test",
                    "version": "1.0.0"
                }
            }
        }),
    )
    .await;

    let initialized = receive_message(&mut lines).await;
    assert_eq!(initialized["id"], 42);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "reading-mcp");

    send_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    )
    .await;
    send_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 43,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await;

    let tools = receive_message(&mut lines).await;
    let tool_names = tools["result"]["tools"]
        .as_array()
        .expect("tools/list should return an array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(tools["id"], 43);
    assert!(tool_names.contains(&"list_documents"));

    drop(stdin);
    let status = timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("reading-mcp should stop after stdin closes")
        .expect("reading-mcp process should be waitable");
    assert!(status.success());
}

async fn send_message(stdin: &mut tokio::process::ChildStdin, message: Value) {
    stdin
        .write_all(format!("{message}\n").as_bytes())
        .await
        .expect("JSON-RPC message should be written");
    stdin.flush().await.expect("JSON-RPC message should flush");
}

async fn receive_message(lines: &mut Lines<BufReader<ChildStdout>>) -> Value {
    let line = timeout(Duration::from_secs(5), lines.next_line())
        .await
        .expect("reading-mcp should respond before timeout")
        .expect("reading-mcp stdout should be readable")
        .expect("reading-mcp stdout should remain open");
    serde_json::from_str(&line).expect("reading-mcp response should be valid JSON")
}
