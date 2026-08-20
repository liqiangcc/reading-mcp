use std::process::Stdio;
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::Value;
use tokio::process::{Child, Command};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn http_transport_enforces_auth_and_origin_while_exposing_local_probes() {
    let port = reserve_port();
    let mut child = spawn_server(port);
    let base = format!("http://127.0.0.1:{port}");
    wait_until_ready(&base, &mut child).await;

    let health = reqwest::get(format!("{base}/healthz"))
        .await
        .expect("health endpoint should respond");
    assert_eq!(health.status(), StatusCode::OK);
    let health_text = health.text().await.expect("health body should be readable");
    let health_body: Value =
        serde_json::from_str(&health_text).expect("health response should be JSON");
    assert_eq!(health_body["status"], "ok");
    assert_eq!(health_body["transport"], "streamable-http");

    let ready = reqwest::get(format!("{base}/readyz"))
        .await
        .expect("ready endpoint should respond");
    assert_eq!(ready.status(), StatusCode::OK);
    let ready_text = ready.text().await.expect("ready body should be readable");
    let ready_body: Value =
        serde_json::from_str(&ready_text).expect("ready response should be JSON");
    assert_eq!(ready_body["status"], "ready");
    assert_eq!(ready_body["mcp_path"], "/mcp");

    let client = reqwest::Client::new();
    let unauthorized = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("unauthorized request should receive a response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let hostile_host = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TOKEN)
        .header("Host", "evil.example")
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("hostile Host request should receive a response");
    assert_eq!(hostile_host.status(), StatusCode::FORBIDDEN);

    let hostile_origin = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TOKEN)
        .header("Origin", "https://evil.example")
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("hostile Origin request should receive a response");
    assert_eq!(hostile_origin.status(), StatusCode::FORBIDDEN);

    let allowed_origin = client
        .post(format!("{base}/mcp"))
        .bearer_auth(TOKEN)
        .header("Origin", format!("http://127.0.0.1:{port}"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("allowed Origin request should receive a response");
    assert_ne!(allowed_origin.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(allowed_origin.status(), StatusCode::FORBIDDEN);

    child.kill().await.expect("HTTP server should stop");
}

#[tokio::test]
async fn http_transport_refuses_non_loopback_bind() {
    let output = Command::new(env!("CARGO_BIN_EXE_reading-mcp-http"))
        .env("READING_MCP_HTTP_TOKEN", TOKEN)
        .env("READING_MCP_HTTP_BIND", "0.0.0.0:8787")
        .env("READING_MCP_STATE_DIR", "memory")
        .output()
        .await
        .expect("HTTP binary should execute");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("loopback"), "unexpected stderr: {stderr}");
}

fn reserve_port() -> u16 {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("temporary port should be reserved");
    listener
        .local_addr()
        .expect("listener should have a local address")
        .port()
}

fn spawn_server(port: u16) -> Child {
    Command::new(env!("CARGO_BIN_EXE_reading-mcp-http"))
        .env("READING_MCP_HTTP_TOKEN", TOKEN)
        .env("READING_MCP_HTTP_BIND", format!("127.0.0.1:{port}"))
        .env("READING_MCP_STATE_DIR", "memory")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("HTTP server should start")
}

async fn wait_until_ready(base: &str, child: &mut Child) {
    for _ in 0..60 {
        if let Some(status) = child.try_wait().expect("child status should be readable") {
            panic!("HTTP server exited before becoming ready: {status}");
        }

        if let Ok(response) = reqwest::get(format!("{base}/healthz")).await
            && response.status().is_success()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    panic!("HTTP server did not become ready");
}
