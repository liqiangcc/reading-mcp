use std::{env, net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use reading_mcp::mcp::ReadingMcpServer;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};

const DEFAULT_BIND: &str = "127.0.0.1:8787";
const MCP_PATH: &str = "/mcp";
const HEALTH_PATH: &str = "/healthz";
const READY_PATH: &str = "/readyz";

#[derive(Clone)]
struct AuthState {
    token: Arc<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if reading_mcp::parsing::run_file_source_view_worker_if_requested()? {
        return Ok(());
    }
    if reading_mcp::parsing::run_source_view_worker_if_requested()? {
        return Ok(());
    }

    let token = env::var("READING_MCP_HTTP_TOKEN")
        .map_err(|_| "READING_MCP_HTTP_TOKEN must be set for the HTTP server")?;
    if token.len() < 32 {
        return Err("READING_MCP_HTTP_TOKEN must contain at least 32 characters".into());
    }

    let bind = env::var("READING_MCP_HTTP_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    let address: SocketAddr = bind.parse()?;
    validate_bind(address)?;

    let mut http_config = StreamableHttpServerConfig::default()
        .with_json_response(true)
        .with_stateful_mode(true)
        .with_allowed_origins(default_loopback_origins(address.port()));

    if let Ok(value) = env::var("READING_MCP_HTTP_ALLOWED_HOSTS") {
        let hosts = parse_csv(&value);
        if hosts.is_empty() {
            return Err("READING_MCP_HTTP_ALLOWED_HOSTS must not be empty when set".into());
        }
        http_config = http_config.with_allowed_hosts(hosts);
    }

    if let Ok(value) = env::var("READING_MCP_HTTP_ALLOWED_ORIGINS") {
        let origins = parse_csv(&value);
        if origins.is_empty() {
            return Err("READING_MCP_HTTP_ALLOWED_ORIGINS must not be empty when set".into());
        }
        http_config = http_config.with_allowed_origins(origins);
    }

    let service = StreamableHttpService::new(
        || ReadingMcpServer::from_env().map_err(std::io::Error::other),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );
    let auth = AuthState {
        token: Arc::new(token),
    };

    let protected = Router::new()
        .nest_service(MCP_PATH, service)
        .layer(middleware::from_fn_with_state(auth, authorize));
    let app = Router::new()
        .route("/health", get(legacy_health))
        .route(HEALTH_PATH, get(healthz))
        .route(READY_PATH, get(readyz))
        .merge(protected);

    let listener = tokio::net::TcpListener::bind(address).await?;
    let bound = listener.local_addr()?;
    eprintln!(
        "{}",
        json!({
            "event": "mcp_http_listen",
            "endpoint": format!("http://{bound}{MCP_PATH}"),
            "health": format!("http://{bound}{HEALTH_PATH}"),
            "ready": format!("http://{bound}{READY_PATH}"),
            "remote_access": "use a trusted tunnel or reverse proxy; direct non-loopback bind is disabled"
        })
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn legacy_health() -> &'static str {
    "ok"
}

async fn healthz() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "reading-mcp",
        "transport": "streamable-http"
    }))
}

async fn readyz() -> Json<Value> {
    Json(json!({
        "status": "ready",
        "service": "reading-mcp",
        "transport": "streamable-http",
        "mcp_path": MCP_PATH
    }))
}

async fn authorize(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let valid = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|candidate| candidate == auth.token.as_str());

    if valid {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn validate_bind(address: SocketAddr) -> Result<(), &'static str> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(
            "READING_MCP_HTTP_BIND must use a loopback address; use a trusted tunnel or reverse proxy for remote access",
        )
    }
}

fn default_loopback_origins(port: u16) -> Vec<String> {
    vec![
        format!("http://localhost:{port}"),
        format!("http://127.0.0.1:{port}"),
        format!("http://[::1]:{port}"),
    ]
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{default_loopback_origins, parse_csv, validate_bind};

    #[test]
    fn rejects_non_loopback_bind() {
        assert!(validate_bind("127.0.0.1:8787".parse().unwrap()).is_ok());
        assert!(validate_bind("[::1]:8787".parse().unwrap()).is_ok());
        assert!(validate_bind("0.0.0.0:8787".parse().unwrap()).is_err());
        assert!(validate_bind("192.168.1.20:8787".parse().unwrap()).is_err());
    }

    #[test]
    fn loopback_origins_follow_bound_port() {
        assert_eq!(
            default_loopback_origins(8787),
            vec![
                "http://localhost:8787",
                "http://127.0.0.1:8787",
                "http://[::1]:8787",
            ]
        );
    }

    #[test]
    fn csv_configuration_ignores_empty_entries() {
        assert_eq!(
            parse_csv("example.com, tunnel.example.com , ,localhost"),
            vec!["example.com", "tunnel.example.com", "localhost"]
        );
    }
}
